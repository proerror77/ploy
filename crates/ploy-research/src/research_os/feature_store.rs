//! Research OS feature snapshot contract.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::research_os::runtime_inputs::runtime_input_contract;
use crate::research_snapshot::ResearchSnapshotManifest;

pub const FEATURE_SNAPSHOT_MANIFEST_SCHEMA_VERSION: &str = "feature_snapshot_manifest_v1";
pub const FEATURE_SNAPSHOT_MANIFEST_ARTIFACT: &str = "feature-snapshot-manifest.json";
pub const MISSING_BLOCKS_PROMOTION_PREFIX: &str = "missing_blocks_promotion:surface:";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureSnapshotWindow {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub history_start: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureSurface {
    pub name: String,
    pub category: String,
    pub required_for_prediction: bool,
    pub required_for_execution: bool,
    pub present: bool,
    pub row_count: usize,
    pub blockers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureSnapshotManifest {
    pub schema_version: String,
    pub snapshot_id: String,
    pub source_run_id: Option<String>,
    pub source_artifact: Option<String>,
    pub source_snapshot_hash: Option<String>,
    pub window: FeatureSnapshotWindow,
    pub symbols: Vec<String>,
    pub surfaces: Vec<FeatureSurface>,
    pub feature_schema_hash: String,
    pub blockers: Vec<String>,
}

impl FeatureSnapshotManifest {
    pub fn surface(&self, name: &str) -> Option<&FeatureSurface> {
        self.surfaces.iter().find(|surface| surface.name == name)
    }
}

pub fn feature_snapshot_manifest_from_research_snapshot(
    manifest: &ResearchSnapshotManifest,
) -> FeatureSnapshotManifest {
    let source_run_id = std::env::var("GITHUB_RUN_ID").ok();
    let source_artifact = source_run_id
        .as_ref()
        .map(|run_id| format!("research-snapshot-{run_id}"));
    feature_snapshot_manifest_from_research_snapshot_with_source(
        manifest,
        source_run_id,
        source_artifact,
    )
}

pub fn feature_snapshot_manifest_from_research_snapshot_with_source(
    manifest: &ResearchSnapshotManifest,
    source_run_id: Option<String>,
    source_artifact: Option<String>,
) -> FeatureSnapshotManifest {
    let requirements = manifest
        .data_requirements
        .iter()
        .map(|value| value.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    let all_required = requirements.contains("all");
    let required = |name: &str| all_required || requirements.contains(name);

    let mut surfaces = vec![
        feature_surface(
            "factor_observations",
            "derived_feature_rows",
            !manifest.data_requirements.is_empty(),
            false,
            manifest.row_counts.observations > 0,
            manifest.row_counts.observations,
        ),
        feature_surface(
            "binance_price",
            "cex_price",
            required("binance_price"),
            false,
            manifest.row_counts.observations > 0,
            manifest.row_counts.observations,
        ),
        feature_surface(
            "binance_agg_trades",
            "cex_trade_flow",
            required("binance_agg_trades"),
            false,
            manifest.row_counts.observations > 0,
            manifest.row_counts.observations,
        ),
        feature_surface(
            "binance_lob",
            "cex_lob",
            required("binance_lob"),
            false,
            manifest.row_counts.lob_snapshots > 0,
            manifest.row_counts.lob_snapshots,
        ),
        feature_surface(
            "polymarket_quotes",
            "pm_top_of_book",
            required("polymarket_quotes"),
            true,
            manifest.row_counts.observations > 0,
            manifest.row_counts.observations,
        ),
        feature_surface(
            "polymarket_orderbooks",
            "pm_full_depth",
            required("polymarket_orderbooks"),
            true,
            manifest.row_counts.pm_book_snapshots > 0,
            manifest.row_counts.pm_book_snapshots,
        ),
        feature_surface(
            "deribit_iv",
            "deribit_volatility_surface",
            required("deribit_iv") || required("deribit_atm_greeks"),
            false,
            manifest.row_counts.deribit_snapshots > 0,
            manifest.row_counts.deribit_snapshots,
        ),
        feature_surface(
            "official_settlement",
            "settlement_label",
            manifest.require_official_settlement,
            false,
            manifest.require_official_settlement && manifest.row_counts.observations > 0,
            manifest.row_counts.observations,
        ),
    ];
    surfaces.sort_by(|lhs, rhs| lhs.name.cmp(&rhs.name));

    let mut blockers = surfaces
        .iter()
        .flat_map(|surface| surface.blockers.iter().cloned())
        .collect::<Vec<_>>();
    if let Some(status) = manifest.data_audit_status.as_deref() {
        if !matches!(status, "ok" | "passed" | "pass") {
            blockers.push(format!(
                "missing_blocks_promotion:data_audit_status:{status}"
            ));
        }
    }
    blockers.sort();
    blockers.dedup();

    let mut feature_manifest = FeatureSnapshotManifest {
        schema_version: FEATURE_SNAPSHOT_MANIFEST_SCHEMA_VERSION.to_string(),
        snapshot_id: manifest
            .snapshot_hash
            .clone()
            .unwrap_or_else(|| "pending_snapshot_hash".to_string()),
        source_run_id,
        source_artifact,
        source_snapshot_hash: manifest.snapshot_hash.clone(),
        window: FeatureSnapshotWindow {
            start: manifest.start,
            end: manifest.end,
            history_start: manifest.history_start,
        },
        symbols: manifest.symbols.clone(),
        surfaces,
        feature_schema_hash: String::new(),
        blockers,
    };
    feature_manifest.feature_schema_hash = compute_feature_schema_hash(&feature_manifest);
    feature_manifest
}

pub fn feature_surface_blockers_for_runtime_inputs<'a>(
    input_names: impl IntoIterator<Item = &'a str>,
    manifest: &FeatureSnapshotManifest,
) -> Vec<String> {
    let mut blockers = Vec::new();
    for input_name in input_names {
        let Some(input_contract) = runtime_input_contract(input_name) else {
            continue;
        };
        for surface_name in feature_surfaces_for_runtime_source(input_contract.source_surface) {
            match manifest.surface(surface_name) {
                Some(surface) if surface.present && surface.blockers.is_empty() => {}
                Some(surface) => blockers.extend(surface.blockers.iter().cloned()),
                None => blockers.push(format!("{MISSING_BLOCKS_PROMOTION_PREFIX}{surface_name}")),
            }
        }
    }
    blockers.sort();
    blockers.dedup();
    blockers
}

fn feature_surface(
    name: &str,
    category: &str,
    required_for_prediction: bool,
    required_for_execution: bool,
    present: bool,
    row_count: usize,
) -> FeatureSurface {
    let mut blockers = Vec::new();
    if (required_for_prediction || required_for_execution) && !present {
        blockers.push(format!("{MISSING_BLOCKS_PROMOTION_PREFIX}{name}"));
    }
    FeatureSurface {
        name: name.to_string(),
        category: category.to_string(),
        required_for_prediction,
        required_for_execution,
        present,
        row_count,
        blockers,
    }
}

fn feature_surfaces_for_runtime_source(source_surface: &str) -> &'static [&'static str] {
    match source_surface {
        "polymarket_full_depth_and_fair_probability"
        | "polymarket_conservative_depth_and_fair_probability"
        | "polymarket_full_depth_and_external_model_probability"
        | "polymarket_conservative_depth_and_external_model_probability"
        | "polymarket_full_depth" => &["polymarket_orderbooks"],
        "polymarket_top_of_book" => &["polymarket_quotes"],
        "binance_spot_ticks" => &["binance_price"],
        "binance_spot_ticks_plus_polymarket_quote_age" => &["binance_price", "polymarket_quotes"],
        "research_lob_composite" => &["binance_lob"],
        "deribit_volatility_surface" => &["deribit_iv"],
        "event_distance_to_strike" | "event_volatility_state" | "research_composite" => {
            &["factor_observations"]
        }
        _ => &[],
    }
}

fn compute_feature_schema_hash(manifest: &FeatureSnapshotManifest) -> String {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    fn update(hash: &mut u64, bytes: &[u8]) {
        for byte in bytes {
            *hash ^= u64::from(*byte);
            *hash = hash.wrapping_mul(FNV_PRIME);
        }
    }

    let mut hash = FNV_OFFSET;
    update(
        &mut hash,
        FEATURE_SNAPSHOT_MANIFEST_SCHEMA_VERSION.as_bytes(),
    );
    update(&mut hash, manifest.snapshot_id.as_bytes());
    update(&mut hash, manifest.window.start.to_rfc3339().as_bytes());
    update(&mut hash, manifest.window.end.to_rfc3339().as_bytes());
    update(&mut hash, manifest.symbols.join(",").as_bytes());
    for surface in &manifest.surfaces {
        update(&mut hash, surface.name.as_bytes());
        update(&mut hash, surface.category.as_bytes());
        update(
            &mut hash,
            surface.required_for_prediction.to_string().as_bytes(),
        );
        update(
            &mut hash,
            surface.required_for_execution.to_string().as_bytes(),
        );
        update(&mut hash, surface.present.to_string().as_bytes());
        update(&mut hash, surface.row_count.to_string().as_bytes());
    }
    format!("{hash:016x}")
}

pub fn feature_surface_status_map(
    manifest: &FeatureSnapshotManifest,
) -> BTreeMap<String, FeatureSurface> {
    manifest
        .surfaces
        .iter()
        .map(|surface| (surface.name.clone(), surface.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::research_snapshot::{
        ResearchSnapshotArtifacts, ResearchSnapshotManifest, ResearchSnapshotRowCounts,
        RESEARCH_SNAPSHOT_SCHEMA_VERSION,
    };

    fn manifest_with_counts(
        observations: usize,
        deribit_snapshots: usize,
        pm_book_snapshots: usize,
        data_requirements: Vec<&str>,
    ) -> ResearchSnapshotManifest {
        let start = "2026-04-24T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let end = "2026-05-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        ResearchSnapshotManifest {
            schema_version: RESEARCH_SNAPSHOT_SCHEMA_VERSION.to_string(),
            snapshot_hash: Some("snapshot-1".to_string()),
            generated_at: start,
            git_sha: Some("test-sha".to_string()),
            symbols: vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()],
            start,
            end,
            history_start: start,
            lob_sample_secs: 30,
            pm_book_sample_secs: Some(300),
            observation_sample_secs: 30,
            max_quote_age_secs: 30,
            stake_usd: 15.0,
            require_official_settlement: true,
            immutable_input: true,
            source_kind: "unit_test".to_string(),
            optimizer_data_dir: Some("/tmp/immutable-parquet".to_string()),
            data_requirements: data_requirements.into_iter().map(str::to_string).collect(),
            data_audit_status: Some("ok".to_string()),
            data_audit_report: Some("data-gap-audit.json".to_string()),
            include_deribit: true,
            artifacts: ResearchSnapshotArtifacts::default(),
            row_counts: ResearchSnapshotRowCounts {
                observations,
                lob_snapshots: observations,
                deribit_snapshots,
                pm_book_snapshots,
            },
            phase_timings: vec![],
            quality_flags: vec![],
        }
    }

    #[test]
    fn feature_store_marks_missing_required_surface_as_promotion_blocker() {
        let research_manifest = manifest_with_counts(
            10,
            0,
            0,
            vec!["polymarket_quotes", "polymarket_orderbooks", "binance_lob"],
        );
        let feature_manifest = feature_snapshot_manifest_from_research_snapshot_with_source(
            &research_manifest,
            Some("123".to_string()),
            Some("research-snapshot-123".to_string()),
        );

        assert_eq!(feature_manifest.source_run_id.as_deref(), Some("123"));
        assert_eq!(
            feature_manifest
                .surface("polymarket_orderbooks")
                .unwrap()
                .blockers,
            vec!["missing_blocks_promotion:surface:polymarket_orderbooks"]
        );
        assert!(feature_manifest
            .blockers
            .contains(&"missing_blocks_promotion:surface:polymarket_orderbooks".to_string()));
        assert!(!feature_manifest.feature_schema_hash.is_empty());
    }

    #[test]
    fn feature_store_maps_runtime_inputs_to_missing_surface_blockers() {
        let mut research_manifest = manifest_with_counts(
            10,
            0,
            0,
            vec!["polymarket_quotes", "polymarket_orderbooks", "binance_lob"],
        );
        research_manifest.row_counts.lob_snapshots = 0;
        let feature_manifest = feature_snapshot_manifest_from_research_snapshot_with_source(
            &research_manifest,
            None,
            None,
        );

        let blockers = feature_surface_blockers_for_runtime_inputs(
            ["full_depth_settlement_edge", "external_pressure"],
            &feature_manifest,
        );

        assert_eq!(
            blockers,
            vec![
                "missing_blocks_promotion:surface:binance_lob",
                "missing_blocks_promotion:surface:polymarket_orderbooks"
            ]
        );
    }
}
