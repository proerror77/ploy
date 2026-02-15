use crate::error::{PloyError, Result};
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArenaTextEntry {
    pub rank: u32,
    pub model: String,
    pub score: i32,
    pub org: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArenaTextSnapshot {
    /// The "Last Updated" date displayed on the page (date only).
    pub last_updated: Option<NaiveDate>,
    /// When we fetched the snapshot.
    pub fetched_at: DateTime<Utc>,
    /// Parsed leaderboard entries (typically many rows).
    pub entries: Vec<ArenaTextEntry>,
    /// Raw source URL used to fetch.
    pub source_url: String,
}

impl ArenaTextSnapshot {
    /// Best (max) score per organization across parsed entries.
    pub fn best_score_by_org(&self) -> HashMap<String, i32> {
        let mut map: HashMap<String, i32> = HashMap::new();
        for e in &self.entries {
            map.entry(e.org.clone())
                .and_modify(|v| *v = (*v).max(e.score))
                .or_insert(e.score);
        }
        map
    }

    pub fn top_org(&self) -> Option<String> {
        let mut best: Option<(&str, i32)> = None;
        for e in &self.entries {
            match best {
                None => best = Some((e.org.as_str(), e.score)),
                Some((_, s)) if e.score > s => best = Some((e.org.as_str(), e.score)),
                _ => {}
            }
        }
        best.map(|(o, _)| o.to_string())
    }

    pub fn staleness_days(&self) -> Option<f64> {
        let d = self.last_updated?;
        let last = d.and_hms_opt(0, 0, 0)?;
        let last = DateTime::<Utc>::from_naive_utc_and_offset(last, Utc);
        let delta = self.fetched_at - last;
        Some(delta.num_seconds().max(0) as f64 / 86_400.0)
    }
}

/// Fetches and parses the Arena "Text" leaderboard via the Jina AI proxy.
///
/// We use the proxy because the origin site is heavily JS/CF-protected.
pub async fn fetch_arena_text_snapshot() -> Result<ArenaTextSnapshot> {
    let source_url = "https://arena.ai/leaderboard/text";
    let url = format!("https://r.jina.ai/{}", source_url);
    let fetched_at = Utc::now();

    let body = reqwest::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| PloyError::Internal(format!("Arena fetch failed: {e}")))?
        .error_for_status()
        .map_err(|e| PloyError::Internal(format!("Arena fetch bad status: {e}")))?
        .text()
        .await
        .map_err(|e| PloyError::Internal(format!("Arena fetch body failed: {e}")))?;

    let (last_updated, entries) = parse_arena_text_markdown(&body)?;

    Ok(ArenaTextSnapshot {
        last_updated,
        fetched_at,
        entries,
        source_url: source_url.to_string(),
    })
}

fn parse_last_updated(lines: &[&str]) -> Option<NaiveDate> {
    for (i, line) in lines.iter().enumerate() {
        if line.trim() != "Last Updated" {
            continue;
        }

        let mut j = i + 1;
        while j < lines.len() && lines[j].trim().is_empty() {
            j += 1;
        }
        let date_str = lines.get(j)?.trim();

        // Examples observed: "Feb 6, 2026"
        return NaiveDate::parse_from_str(date_str, "%b %e, %Y")
            .or_else(|_| NaiveDate::parse_from_str(date_str, "%b %d, %Y"))
            .ok();
    }

    None
}

fn parse_markdown_link_label(s: &str) -> String {
    let s = s.trim();
    if let (Some(lb), Some(rb)) = (s.find('['), s.find("](")) {
        if rb > lb + 1 {
            return s[lb + 1..rb].trim().to_string();
        }
    }
    s.to_string()
}

/// Parse Arena Text leaderboard markdown into (last_updated, entries).
pub fn parse_arena_text_markdown(
    markdown: &str,
) -> Result<(Option<NaiveDate>, Vec<ArenaTextEntry>)> {
    let lines: Vec<&str> = markdown.lines().collect();
    let last_updated = parse_last_updated(&lines);

    let mut entries = Vec::new();
    for line in &lines {
        let l = line.trim();
        if !l.starts_with('|') {
            continue;
        }
        // Skip header separator rows like "| --- | --- |"
        if l.contains("---") && !l.chars().any(|c| c.is_ascii_digit()) {
            continue;
        }

        let parts: Vec<&str> = l.split('|').map(|p| p.trim()).collect();
        // Expected columns (with empty at ends):
        // 1 rank, 2 change, 3 model, 4 score, 5 CI, 6 votes, 7 org, 8 license
        if parts.len() < 9 {
            continue;
        }

        let rank = match parts.get(1).and_then(|s| s.parse::<u32>().ok()) {
            Some(r) if r > 0 => r,
            _ => continue,
        };

        let model_cell = parts.get(3).unwrap_or(&"");
        let score_cell = parts.get(4).unwrap_or(&"");
        let org_cell = parts.get(7).unwrap_or(&"");

        let score: i32 = score_cell.replace(',', "").parse().map_err(|e| {
            PloyError::Internal(format!("Arena parse score failed ({score_cell}): {e}"))
        })?;

        let model = parse_markdown_link_label(model_cell);
        let org = org_cell.to_string();

        entries.push(ArenaTextEntry {
            rank,
            model,
            score,
            org,
        });
    }

    if entries.is_empty() {
        return Err(PloyError::Internal(
            "Arena parse produced no entries (format changed?)".to_string(),
        ));
    }

    Ok((last_updated, entries))
}

/// Convert softmax-like weights into Decimal probabilities for a set of org scores.
///
/// - `scores` is org -> best score (higher is better)
/// - `temp` controls softness (higher temp => flatter distribution)
pub fn scores_to_probabilities(
    scores: &HashMap<String, i32>,
    temp: f64,
) -> HashMap<String, Decimal> {
    let mut result = HashMap::new();
    if scores.is_empty() {
        return result;
    }
    let max_score = scores.values().copied().max().unwrap_or(0) as f64;

    let mut weights: HashMap<String, f64> = HashMap::new();
    let mut sum = 0.0f64;
    for (org, s) in scores {
        let w = (((*s as f64) - max_score) / temp.max(1e-9)).exp();
        weights.insert(org.clone(), w);
        sum += w;
    }

    if sum <= 0.0 {
        let n = scores.len() as f64;
        for org in scores.keys() {
            result.insert(
                org.clone(),
                Decimal::from_f64(1.0 / n).unwrap_or(Decimal::ZERO),
            );
        }
        return result;
    }

    for (org, w) in weights {
        let p = w / sum;
        result.insert(org, Decimal::from_f64(p).unwrap_or(Decimal::ZERO));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_last_updated_and_entries() {
        let md = r#"
Text Arena
==========

Last Updated

Feb 6, 2026

| Rank | Change | Model | Score | CI | Votes | Organization | License |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | 1◄─►2 | [claude-opus-4-6](https://www.anthropic.com/news/claude-opus-4-6 "claude-opus-4-6") | 1496 | ±11 | 2,829 | Anthropic | Proprietary |
| 2 | 1◄─►2 | [gemini-3-pro](https://aistudio.google.com/ "gemini-3-pro") | 1486 | ±9 | 34,419 | Google | Proprietary |
"#;
        let (d, entries) = parse_arena_text_markdown(md).unwrap();
        assert_eq!(d.unwrap(), NaiveDate::from_ymd_opt(2026, 2, 6).unwrap());
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].rank, 1);
        assert_eq!(entries[0].model, "claude-opus-4-6");
        assert_eq!(entries[0].score, 1496);
        assert_eq!(entries[0].org, "Anthropic");
    }
}
