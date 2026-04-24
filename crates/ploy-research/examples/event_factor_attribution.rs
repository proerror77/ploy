use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use ploy_research::{
    AutomlFactorAttribution, DatasetBuildManifest, FactorRegistry, Regime,
    register_automl_attributions,
};
use polars::io::parquet::read::ParquetReader;
use polars::prelude::*;

const DEFAULT_FEATURES: &[&str] = &[
    "signed_distance_to_beat",
    "abs_distance_to_beat",
    "drift_10s",
    "drift_30s",
    "flip_age_secs",
    "post_flip_drift",
    "sigma_horizon",
    "fair_prob_up",
    "fair_prob_up_clean",
    "prob_disagreement",
    "implied_sigma_horizon",
    "vol_gap",
    "distance_over_sigma",
    "model_prob_up",
    "model_edge_up",
    "reward_risk_up",
    "reward_risk_down",
    "obi",
    "spread_bps",
    "microprice_offset_bps",
    "depth_ratio",
    "depth_imbalance",
    "depth_far_ratio",
    "obi_10",
    "pm_lag_secs",
];

#[derive(Debug, Clone)]
struct Config {
    dataset_root: PathBuf,
    entry_secs: i64,
    tolerance_secs: i64,
    top_n: usize,
    features: Vec<String>,
}

#[derive(Debug, Clone)]
struct Sample {
    y: f64,
    x: Vec<f64>,
}

#[derive(Debug, Clone)]
struct SplitDataset {
    name: &'static str,
    samples: Vec<Sample>,
}

#[derive(Debug, Clone)]
struct FactorAttribution {
    feature: String,
    direction: i8,
    train_auc_lift: f64,
    val_auc_lift: f64,
    test_auc_lift: f64,
    importance: f64,
    stability: f64,
    train_corr: f64,
    val_corr: f64,
    test_corr: f64,
}

fn main() -> Result<()> {
    let config = parse_config()?;
    let manifest = read_manifest(&config.dataset_root)?;
    manifest
        .validate_contract()
        .map_err(|message| anyhow::anyhow!(message))
        .context("dataset manifest contract validation failed")?;

    let train = load_split_dataset(
        "train",
        &artifact_path(&config.dataset_root, &manifest.artifacts.observations.train),
        &config.features,
        config.entry_secs,
        config.tolerance_secs,
    )?;
    let val = load_split_dataset(
        "val",
        &artifact_path(&config.dataset_root, &manifest.artifacts.observations.val),
        &config.features,
        config.entry_secs,
        config.tolerance_secs,
    )?;
    let test = load_split_dataset(
        "test",
        &artifact_path(&config.dataset_root, &manifest.artifacts.observations.test),
        &config.features,
        config.entry_secs,
        config.tolerance_secs,
    )?;

    if train.samples.len() < 20 {
        bail!(
            "training sample too small for attribution: {} selected events",
            train.samples.len()
        );
    }

    let attributions = attribute_features(&config.features, &train, &val, &test);
    let regime = Regime::from_secs(config.entry_secs);
    let mut registry = FactorRegistry::new();
    let registry_items = attributions
        .iter()
        .map(|item| AutomlFactorAttribution {
            name: item.feature.clone(),
            importance: item.importance,
            direction: item.direction,
            stability: item.stability,
        })
        .collect::<Vec<_>>();
    register_automl_attributions(&mut registry, regime, "settlement_up", &registry_items);

    print_report(
        &manifest,
        &config,
        [&train, &val, &test],
        &attributions,
        &registry,
        regime,
    );

    Ok(())
}

fn parse_config() -> Result<Config> {
    let args: Vec<String> = std::env::args().collect();
    let dataset_root = flag_value(&args, "--dataset")
        .or_else(|| flag_value(&args, "--dataset-root"))
        .map(PathBuf::from)
        .context("--dataset <event-root-dataset-dir> is required")?;
    let entry_secs = parse_i64_flag(&args, "--entry-secs", 60)?;
    let tolerance_secs = parse_i64_flag(&args, "--tolerance-secs", 30)?;
    let top_n = parse_usize_flag(&args, "--top-n", 12)?;
    let features = flag_value(&args, "--features")
        .map(|raw| parse_string_list(&raw))
        .filter(|features| !features.is_empty())
        .unwrap_or_else(|| {
            DEFAULT_FEATURES
                .iter()
                .map(|name| (*name).to_string())
                .collect()
        });

    if entry_secs < 0 {
        bail!("--entry-secs must be non-negative");
    }
    if tolerance_secs < 0 {
        bail!("--tolerance-secs must be non-negative");
    }
    if top_n == 0 {
        bail!("--top-n must be positive");
    }

    Ok(Config {
        dataset_root,
        entry_secs,
        tolerance_secs,
        top_n,
        features,
    })
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone())
}

fn parse_i64_flag(args: &[String], flag: &str, default: i64) -> Result<i64> {
    flag_value(args, flag)
        .map(|raw| {
            raw.parse::<i64>()
                .with_context(|| format!("invalid {flag}: {raw}"))
        })
        .transpose()
        .map(|value| value.unwrap_or(default))
}

fn parse_usize_flag(args: &[String], flag: &str, default: usize) -> Result<usize> {
    flag_value(args, flag)
        .map(|raw| {
            raw.parse::<usize>()
                .with_context(|| format!("invalid {flag}: {raw}"))
        })
        .transpose()
        .map(|value| value.unwrap_or(default))
}

fn parse_string_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn read_manifest(root: &Path) -> Result<DatasetBuildManifest> {
    let path = root.join("event_manifest.json");
    let file = File::open(&path).with_context(|| format!("open manifest {}", path.display()))?;
    serde_json::from_reader(file).with_context(|| format!("parse manifest {}", path.display()))
}

fn artifact_path(root: &Path, artifact: &str) -> PathBuf {
    let path = Path::new(artifact);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

fn load_split_dataset(
    name: &'static str,
    path: &Path,
    feature_names: &[String],
    entry_secs: i64,
    tolerance_secs: i64,
) -> Result<SplitDataset> {
    let file = File::open(path).with_context(|| format!("open parquet {}", path.display()))?;
    let df = ParquetReader::new(file)
        .finish()
        .with_context(|| format!("read parquet {}", path.display()))?;

    let event_ids = df.column("event_id")?.str()?;
    let time_remaining = df.column("time_remaining_secs")?.i64()?;
    let labels = df.column("settlement_up")?.f64()?;
    let pm_up_ask = df.column("pm_up_ask")?.f64()?;
    let pm_down_ask = df.column("pm_down_ask")?.f64()?;
    let feature_columns = feature_names
        .iter()
        .map(|feature| {
            df.column(feature)
                .with_context(|| format!("feature column missing: {feature}"))?
                .f64()
                .with_context(|| format!("feature column must be f64: {feature}"))
        })
        .collect::<Result<Vec<_>>>()?;

    let mut by_event: BTreeMap<String, (i64, Sample)> = BTreeMap::new();
    for row_idx in 0..df.height() {
        let Some(event_id) = event_ids.get(row_idx) else {
            continue;
        };
        let Some(seconds) = time_remaining.get(row_idx) else {
            continue;
        };
        let distance = (seconds - entry_secs).abs();
        if distance > tolerance_secs {
            continue;
        }

        let Some(y) = labels.get(row_idx) else {
            continue;
        };
        let Some(up_ask) = pm_up_ask.get(row_idx) else {
            continue;
        };
        let Some(down_ask) = pm_down_ask.get(row_idx) else {
            continue;
        };
        if !is_binary_label(y) || !valid_entry_price(up_ask) || !valid_entry_price(down_ask) {
            continue;
        }

        let mut x = Vec::with_capacity(feature_columns.len());
        let mut valid = true;
        for column in &feature_columns {
            let value = column.get(row_idx).unwrap_or(f64::NAN);
            if value.is_finite() {
                x.push(value);
            } else {
                valid = false;
                break;
            }
        }
        if !valid {
            continue;
        }

        let sample = Sample { y, x };
        match by_event.get(event_id) {
            Some((current_distance, _)) if *current_distance <= distance => {}
            _ => {
                by_event.insert(event_id.to_string(), (distance, sample));
            }
        }
    }

    Ok(SplitDataset {
        name,
        samples: by_event.into_values().map(|(_, sample)| sample).collect(),
    })
}

fn is_binary_label(value: f64) -> bool {
    value.is_finite() && (value == 0.0 || value == 1.0)
}

fn valid_entry_price(value: f64) -> bool {
    value.is_finite() && value > 0.0 && value < 1.0
}

fn attribute_features(
    feature_names: &[String],
    train: &SplitDataset,
    val: &SplitDataset,
    test: &SplitDataset,
) -> Vec<FactorAttribution> {
    let mut attributions = feature_names
        .iter()
        .enumerate()
        .map(|(feature_idx, feature)| {
            let train_auc_lift = signed_auc_lift(train, feature_idx);
            let val_auc_lift = signed_auc_lift(val, feature_idx);
            let test_auc_lift = signed_auc_lift(test, feature_idx);
            let direction = if train_auc_lift >= 0.0 { 1 } else { -1 };
            let importance = val_auc_lift.abs();
            let stability = stability_score(train_auc_lift, val_auc_lift, test_auc_lift);

            FactorAttribution {
                feature: feature.clone(),
                direction,
                train_auc_lift,
                val_auc_lift,
                test_auc_lift,
                importance,
                stability,
                train_corr: pearson_corr(train, feature_idx),
                val_corr: pearson_corr(val, feature_idx),
                test_corr: pearson_corr(test, feature_idx),
            }
        })
        .collect::<Vec<_>>();

    attributions.sort_by(|left, right| {
        right
            .importance
            .partial_cmp(&left.importance)
            .unwrap_or(Ordering::Equal)
            .then_with(|| {
                right
                    .stability
                    .partial_cmp(&left.stability)
                    .unwrap_or(Ordering::Equal)
            })
    });
    attributions
}

fn signed_auc_lift(split: &SplitDataset, feature_idx: usize) -> f64 {
    let scored = split
        .samples
        .iter()
        .map(|sample| (sample.x[feature_idx], sample.y))
        .collect::<Vec<_>>();
    let auc = auc_pairwise(&scored);
    if auc.is_finite() { auc - 0.5 } else { f64::NAN }
}

fn stability_score(train: f64, val: f64, test: f64) -> f64 {
    if !train.is_finite() || !val.is_finite() || !test.is_finite() {
        return 0.0;
    }
    let sign = train.signum();
    if ![val, test]
        .iter()
        .all(|score| score.signum() == sign && score.abs() > 0.0)
    {
        return 0.0;
    }
    let magnitude = val.abs().min(test.abs()) / train.abs().max(1e-9);
    magnitude.min(1.0)
}

fn pearson_corr(split: &SplitDataset, feature_idx: usize) -> f64 {
    if split.samples.len() < 2 {
        return f64::NAN;
    }

    let n = split.samples.len() as f64;
    let x_mean = split
        .samples
        .iter()
        .map(|sample| sample.x[feature_idx])
        .sum::<f64>()
        / n;
    let y_mean = split.samples.iter().map(|sample| sample.y).sum::<f64>() / n;
    let mut covariance = 0.0;
    let mut x_variance = 0.0;
    let mut y_variance = 0.0;
    for sample in &split.samples {
        let x_delta = sample.x[feature_idx] - x_mean;
        let y_delta = sample.y - y_mean;
        covariance += x_delta * y_delta;
        x_variance += x_delta * x_delta;
        y_variance += y_delta * y_delta;
    }

    let denom = (x_variance * y_variance).sqrt();
    if denom > 1e-12 {
        covariance / denom
    } else {
        f64::NAN
    }
}

fn auc_pairwise(scored: &[(f64, f64)]) -> f64 {
    let positives = scored
        .iter()
        .filter(|(_, label)| *label > 0.5)
        .map(|(score, _)| *score)
        .collect::<Vec<_>>();
    let negatives = scored
        .iter()
        .filter(|(_, label)| *label <= 0.5)
        .map(|(score, _)| *score)
        .collect::<Vec<_>>();

    if positives.is_empty() || negatives.is_empty() {
        return f64::NAN;
    }

    let mut wins = 0.0;
    for positive in &positives {
        for negative in &negatives {
            wins += match positive.partial_cmp(negative).unwrap_or(Ordering::Equal) {
                Ordering::Greater => 1.0,
                Ordering::Equal => 0.5,
                Ordering::Less => 0.0,
            };
        }
    }

    wins / (positives.len() * negatives.len()) as f64
}

fn print_report(
    manifest: &DatasetBuildManifest,
    config: &Config,
    splits: [&SplitDataset; 3],
    attributions: &[FactorAttribution],
    registry: &FactorRegistry,
    regime: Regime,
) {
    eprintln!("=== Event Factor Attribution ===");
    eprintln!("dataset={}", config.dataset_root.display());
    eprintln!(
        "source_window={} -> {} symbols={:?}",
        manifest.source_window.start_time,
        manifest.source_window.end_time,
        manifest.source_window.symbols
    );
    eprintln!(
        "entry_secs={} tolerance_secs={} regime={:?} features={}",
        config.entry_secs,
        config.tolerance_secs,
        regime,
        config.features.len()
    );
    eprintln!(
        "selected_events train={} val={} test={}",
        format_split_count(splits[0]),
        format_split_count(splits[1]),
        format_split_count(splits[2])
    );

    eprintln!();
    eprintln!(
        "{:<28} {:>3} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9}",
        "feature", "dir", "score", "stab", "tr_auc", "val_auc", "te_auc", "tr_r", "val_r", "test_r"
    );
    for item in attributions.iter().take(config.top_n) {
        eprintln!(
            "{:<28} {:>3} {:>9.4} {:>9.4} {:>9.4} {:>9.4} {:>9.4} {:>9.4} {:>9.4} {:>9.4}",
            item.feature,
            if item.direction >= 0 { "+" } else { "-" },
            item.importance,
            item.stability,
            item.train_auc_lift,
            item.val_auc_lift,
            item.test_auc_lift,
            item.train_corr,
            item.val_corr,
            item.test_corr
        );
    }

    let registered = registry.top_n(regime, "settlement_up", config.top_n);
    eprintln!();
    eprintln!(
        "registered_automl_factors={} top_registry_entries={}",
        registry.all().len(),
        registered.len()
    );
    for meta in registered {
        eprintln!(
            "{:<34} ic={:>8.4} dir={} stability={:>8.4}",
            meta.name,
            meta.ic,
            if meta.direction >= 0 { "+" } else { "-" },
            meta.stability
        );
    }
    eprintln!(
        "\nNote: this is AutoML-style factor attribution, not hyperparameter search. Ranking uses event-held-out validation AUC lift for importance and train direction for registry direction; use it to prioritize factors, not to claim live edge."
    );
}

fn format_split_count(split: &SplitDataset) -> String {
    format!("{}={}", split.name, split.samples.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(samples: &[(f64, &[f64])]) -> SplitDataset {
        SplitDataset {
            name: "test",
            samples: samples
                .iter()
                .map(|(y, x)| Sample {
                    y: *y,
                    x: x.to_vec(),
                })
                .collect(),
        }
    }

    #[test]
    fn auc_pairwise_handles_ties() {
        let scored = [(0.8, 1.0), (0.5, 1.0), (0.5, 0.0), (0.2, 0.0)];
        let auc = auc_pairwise(&scored);

        assert!((auc - 0.875).abs() < 1e-9);
    }

    #[test]
    fn attribution_ranks_validation_signal() {
        let train = split(&[
            (1.0, &[2.0, 0.0]),
            (1.0, &[1.0, 0.0]),
            (0.0, &[0.0, 1.0]),
            (0.0, &[-1.0, 1.0]),
        ]);
        let val = split(&[(1.0, &[3.0, 0.0]), (0.0, &[-2.0, 1.0])]);
        let test = split(&[(1.0, &[4.0, 0.0]), (0.0, &[-3.0, 1.0])]);
        let features = vec!["signal".to_string(), "noise".to_string()];

        let attributions = attribute_features(&features, &train, &val, &test);

        assert_eq!(attributions[0].feature, "signal");
        assert!(attributions[0].importance > 0.0);
        assert!(attributions[0].stability > 0.0);
    }

    #[test]
    fn stability_requires_oos_direction_alignment() {
        assert_eq!(stability_score(0.2, -0.1, 0.1), 0.0);
        assert!(stability_score(0.2, 0.1, 0.1) > 0.0);
    }

    #[test]
    fn parse_string_list_omits_empty_items() {
        assert_eq!(parse_string_list("a,, b, "), vec!["a", "b"]);
    }
}
