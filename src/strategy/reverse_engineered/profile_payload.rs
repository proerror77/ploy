//! Payload fetching and profile snapshot extraction for reverse-engineered paper mode.

use serde_json::Value;

use crate::error::{PloyError, Result};

use super::{to_f64, ProfileSnapshot, ReverseTradeEvent};

fn to_i64(v: &Value) -> i64 {
    if let Some(x) = v.as_i64() {
        return x;
    }
    if let Some(s) = v.as_str() {
        if let Ok(x) = s.parse::<i64>() {
            return x;
        }
    }
    0
}

fn to_string(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn extract_json_object_from_html(body: &str) -> Result<String> {
    let start = body.find("{\"props\":{").ok_or_else(|| {
        PloyError::Validation("unable to locate profile json in html".to_string())
    })?;
    let bytes = body.as_bytes();
    let mut depth: i64 = 0;
    let mut in_str = false;
    let mut escaped = false;
    let mut end: Option<usize> = None;

    for (idx, b) in bytes.iter().enumerate().skip(start) {
        if in_str {
            if escaped {
                escaped = false;
                continue;
            }
            if *b == b'\\' {
                escaped = true;
            } else if *b == b'"' {
                in_str = false;
            }
            continue;
        }

        match *b {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(idx);
                    break;
                }
            }
            _ => {}
        }
    }

    let end_idx = end.ok_or_else(|| {
        PloyError::Validation("unterminated embedded json object in profile html".to_string())
    })?;

    Ok(body[start..=end_idx].to_string())
}

pub(super) async fn fetch_payload(url_or_file: &str) -> Result<Value> {
    if url_or_file.starts_with("http://") || url_or_file.starts_with("https://") {
        let body = reqwest::get(url_or_file).await?.text().await?;
        let json_text = extract_json_object_from_html(&body)?;
        let value: Value = serde_json::from_str(&json_text)?;
        return Ok(value);
    }

    let raw = std::fs::read_to_string(url_or_file)?;
    if raw.trim_start().starts_with('<') {
        let json_text = extract_json_object_from_html(&raw)?;
        let value: Value = serde_json::from_str(&json_text)?;
        Ok(value)
    } else {
        let value: Value = serde_json::from_str(&raw)?;
        Ok(value)
    }
}

fn flatten_pages(data: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    let pages = data
        .as_object()
        .and_then(|obj| obj.get("pages"))
        .and_then(Value::as_array);
    let Some(pages) = pages else {
        return out;
    };
    for page in pages {
        if let Some(rows) = page.as_array() {
            for row in rows {
                out.push(row.clone());
            }
        }
    }
    out
}

pub fn extract_profile_snapshot(payload: &Value) -> Result<ProfileSnapshot> {
    let page_props = payload
        .get("props")
        .and_then(Value::as_object)
        .and_then(|x| x.get("pageProps"))
        .and_then(Value::as_object)
        .ok_or_else(|| {
            PloyError::Validation("invalid payload: missing props.pageProps".to_string())
        })?;

    let address = page_props
        .get("proxyAddress")
        .or_else(|| page_props.get("primaryAddress"))
        .or_else(|| page_props.get("baseAddress"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    let queries = page_props
        .get("dehydratedState")
        .and_then(Value::as_object)
        .and_then(|x| x.get("queries"))
        .and_then(Value::as_array)
        .ok_or_else(|| {
            PloyError::Validation("invalid payload: missing dehydratedState.queries".to_string())
        })?;

    let mut activity_rows: Vec<Value> = Vec::new();
    let mut position_rows: Vec<Value> = Vec::new();

    for q in queries {
        let Some(query_obj) = q.as_object() else {
            continue;
        };
        let Some(query_key) = query_obj.get("queryKey").and_then(Value::as_array) else {
            continue;
        };
        if query_key.len() < 2 {
            continue;
        }
        let key0 = query_key[0].as_str().unwrap_or_default();
        let key1 = query_key[1].as_str().unwrap_or_default();
        let data = query_obj
            .get("state")
            .and_then(Value::as_object)
            .and_then(|x| x.get("data"))
            .cloned()
            .unwrap_or(Value::Null);

        if key0 == "profile" && key1 == "activity" {
            activity_rows.extend(flatten_pages(&data));
        }
        if key0 == "profile" && key1 == "positions" {
            position_rows.extend(flatten_pages(&data));
        }
    }

    let mut activity: Vec<ReverseTradeEvent> = activity_rows
        .into_iter()
        .map(|row| {
            let obj = row.as_object().cloned().unwrap_or_default();
            ReverseTradeEvent {
                event_slug: obj.get("eventSlug").map(to_string).unwrap_or_default(),
                outcome: obj.get("outcome").map(to_string).unwrap_or_default(),
                side: obj.get("side").map(to_string).unwrap_or_default(),
                price: obj.get("price").map(to_f64).unwrap_or_default(),
                size: obj.get("size").map(to_f64).unwrap_or_default(),
                usdc_size: obj.get("usdcSize").map(to_f64).unwrap_or_default(),
                timestamp: obj.get("timestamp").map(to_i64).unwrap_or_default(),
                title: obj.get("title").map(to_string).unwrap_or_default(),
                raw_type: obj.get("type").map(to_string).unwrap_or_default(),
                transaction_hash: obj
                    .get("transactionHash")
                    .map(to_string)
                    .unwrap_or_default(),
            }
        })
        .collect();

    activity.sort_by_key(|x| x.timestamp);

    Ok(ProfileSnapshot {
        address,
        activity,
        positions: position_rows,
    })
}
