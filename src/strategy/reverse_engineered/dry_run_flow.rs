use super::{
    ProfileSnapshot, ReverseState, ReverseTradeEvent, StrategyParams, clamp, parse_event_end_ts,
};
use serde_json::Value;
use std::collections::HashMap;

fn mark_prices_from_positions(positions: &[Value]) -> HashMap<(String, String), f64> {
    let mut out = HashMap::new();
    for row in positions {
        let Some(obj) = row.as_object() else {
            continue;
        };
        let event_slug = obj
            .get("eventSlug")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let outcome = obj
            .get("outcome")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let px = obj.get("curPrice").map(super::to_f64).unwrap_or_default();
        if !event_slug.is_empty() && !outcome.is_empty() && px > 0.0 {
            out.insert((event_slug, outcome), px);
        }
    }
    out
}

fn title_allowed(title: &str, target_assets: &[String]) -> bool {
    if target_assets.is_empty() {
        return true;
    }
    let up = title.to_ascii_uppercase();
    target_assets
        .iter()
        .any(|x| up.contains(&x.to_ascii_uppercase()))
}

fn inventory_down_ratio(
    inventory: &HashMap<(String, String), Vec<(f64, f64)>>,
    prices: &HashMap<(String, String), f64>,
) -> f64 {
    let mut down = 0.0;
    let mut up = 0.0;
    for ((event_slug, outcome), lots) in inventory {
        let Some(px) = prices.get(&(event_slug.clone(), outcome.clone())) else {
            continue;
        };
        let qty: f64 = lots.iter().map(|(q, _)| *q).sum();
        let val = qty * px;
        if outcome.eq_ignore_ascii_case("Down") {
            down += val;
        } else if outcome.eq_ignore_ascii_case("Up") {
            up += val;
        }
    }
    let total = down + up;
    if total <= 0.0 { 0.5 } else { down / total }
}

fn execute_buy(st: &mut ReverseState, params: &StrategyParams, ev: &ReverseTradeEvent) -> bool {
    let usdc = params.min_trade_usdc;
    if params.max_event_usdc > 0.0 {
        let spent = st.event_spend.get(&ev.event_slug).copied().unwrap_or(0.0);
        if spent + usdc > params.max_event_usdc {
            st.result.skipped_orders += 1;
            return false;
        }
    }
    if params.max_total_usdc > 0.0 && st.result.buy_notional + usdc > params.max_total_usdc {
        st.result.skipped_orders += 1;
        return false;
    }
    let qty = usdc / ev.price;
    st.inventory
        .entry((ev.event_slug.clone(), ev.outcome.clone()))
        .or_default()
        .push((qty, ev.price));
    *st.event_spend.entry(ev.event_slug.clone()).or_insert(0.0) += usdc;
    st.result.buy_notional += usdc;
    st.result.executed_buys += 1;
    true
}

fn execute_sell(st: &mut ReverseState, ev: &ReverseTradeEvent, fraction: f64) -> bool {
    let key = (ev.event_slug.clone(), ev.outcome.clone());
    let Some(lots) = st.inventory.get_mut(&key) else {
        st.result.skipped_orders += 1;
        return false;
    };
    let total_qty: f64 = lots.iter().map(|(q, _)| *q).sum();
    if total_qty <= 0.0 {
        st.result.skipped_orders += 1;
        return false;
    }
    let mut remain = total_qty * clamp(fraction, 0.0, 1.0);
    let mut closed = 0.0;
    while remain > 1e-9 && !lots.is_empty() {
        let (lot_qty, lot_px) = lots[0];
        let take = remain.min(lot_qty);
        st.result.realized_pnl += (ev.price - lot_px) * take;
        let left = lot_qty - take;
        remain -= take;
        closed += take;
        if left <= 1e-9 {
            lots.remove(0);
        } else {
            lots[0] = (left, lot_px);
        }
    }
    if closed <= 0.0 {
        st.result.skipped_orders += 1;
        return false;
    }
    st.result.sell_notional += closed * ev.price;
    st.result.executed_sells += 1;
    true
}

pub(super) fn apply_new_ticks(
    st: &mut ReverseState,
    params: &StrategyParams,
    snapshot: &ProfileSnapshot,
    target_assets: &[String],
) {
    let mut ticks: Vec<&ReverseTradeEvent> = snapshot
        .activity
        .iter()
        .filter(|x| x.raw_type == "TRADE" && x.price > 0.0 && x.size > 0.0)
        .collect();
    ticks.sort_by_key(|x| x.timestamp);

    for ev in ticks {
        let key_id = if ev.transaction_hash.is_empty() {
            format!(
                "{}|{}|{}|{:.8}|{:.8}|{}",
                ev.timestamp, ev.event_slug, ev.outcome, ev.price, ev.size, ev.side
            )
        } else {
            format!("{}|{}|{}", ev.transaction_hash, ev.outcome, ev.side)
        };
        if st.seen_trade_ids.contains(&key_id) {
            continue;
        }
        st.seen_trade_ids.insert(key_id);

        if !title_allowed(&ev.title, target_assets) {
            continue;
        }

        st.latest_px
            .insert((ev.event_slug.clone(), ev.outcome.clone()), ev.price);

        let Some(end_ts) = parse_event_end_ts(&ev.event_slug) else {
            continue;
        };
        let tte = end_ts - ev.timestamp;
        if tte < 0 {
            continue;
        }

        let down_ratio = inventory_down_ratio(&st.inventory, &st.latest_px);
        if tte <= params.entry_window_secs {
            if ev.outcome.eq_ignore_ascii_case("Down") {
                if ev.price <= params.down_buy_threshold && down_ratio <= params.hedge_band_high {
                    let _ = execute_buy(st, params, ev);
                }
            } else if ev.outcome.eq_ignore_ascii_case("Up") {
                if ev.price <= params.up_buy_threshold && down_ratio >= params.hedge_band_low {
                    let _ = execute_buy(st, params, ev);
                }
            }
        }

        let inv_key = (ev.event_slug.clone(), ev.outcome.clone());
        if let Some(lots) = st.inventory.get(&inv_key) {
            if !lots.is_empty() {
                let total_qty: f64 = lots.iter().map(|(q, _)| *q).sum();
                if total_qty > 0.0 {
                    let avg_entry =
                        lots.iter().map(|(q, p)| q * p).sum::<f64>() / total_qty.max(1e-9);
                    if ev.price >= avg_entry + params.take_profit {
                        let _ = execute_sell(st, ev, params.scalp_fraction);
                    } else if tte <= 20
                        && ev.price >= avg_entry + (params.take_profit * 0.6).max(0.01)
                    {
                        let _ = execute_sell(st, ev, 0.20);
                    }
                }
            }
        }
    }
}

pub(super) fn refresh_mark_to_market(st: &mut ReverseState, snapshot: &ProfileSnapshot) {
    let mut marks = mark_prices_from_positions(&snapshot.positions);
    for (k, v) in &st.latest_px {
        marks.entry(k.clone()).or_insert(*v);
    }

    st.result.unrealized_pnl = 0.0;
    st.result.open_mark_value = 0.0;
    st.result.down_mark_value = 0.0;
    st.result.up_mark_value = 0.0;

    for ((event_slug, outcome), lots) in &st.inventory {
        let Some(px) = marks.get(&(event_slug.clone(), outcome.clone())) else {
            continue;
        };
        let qty: f64 = lots.iter().map(|(q, _)| *q).sum();
        let cost: f64 = lots.iter().map(|(q, p)| q * p).sum();
        let value = qty * px;

        st.result.open_mark_value += value;
        st.result.unrealized_pnl += value - cost;
        if outcome.eq_ignore_ascii_case("Down") {
            st.result.down_mark_value += value;
        } else if outcome.eq_ignore_ascii_case("Up") {
            st.result.up_mark_value += value;
        }
    }
}
