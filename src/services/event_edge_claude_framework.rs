use crate::adapters::PolymarketClient;
use crate::config::EventEdgeAgentConfig;
use crate::domain::{OrderRequest, Side};
use crate::error::{PloyError, Result};
use crate::strategy::event_edge::core::EventEdgeCore;
use crate::strategy::event_edge::{discover_best_event_id_by_title, scan_event_edge_once, EdgeRow};
use crate::strategy::event_models::arena_text::fetch_arena_text_snapshot;
use chrono::{DateTime, Utc};
use claude_agent_sdk_rs::tool;
use claude_agent_sdk_rs::types::config::{ClaudeAgentOptions, PermissionMode, SystemPrompt};
use claude_agent_sdk_rs::types::mcp::{create_sdk_mcp_server, McpServerConfig, McpServers};
use claude_agent_sdk_rs::{ClaudeClient, ContentBlock, Message};
use futures::StreamExt;
use rust_decimal::prelude::FromPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::{info, warn};

#[derive(Debug, Clone)]
struct TokenGuard {
    ok_to_buy_until: DateTime<Utc>,
    min_edge: Decimal,
    last_seen_ask: Decimal,
    last_seen_p_true: Decimal,
    outcome: String,
    event_id: String,
}

struct FrameworkState {
    core: EventEdgeCore,
    token_guard: HashMap<String, TokenGuard>,
}

/// Claude SDK agent framework runner with MCP tools.
pub struct EventEdgeClaudeFrameworkAgent {
    state: Arc<Mutex<FrameworkState>>,
}

impl EventEdgeClaudeFrameworkAgent {
    pub fn new(client: PolymarketClient, cfg: EventEdgeAgentConfig) -> Self {
        let st = FrameworkState {
            core: EventEdgeCore::new(client, cfg),
            token_guard: HashMap::new(),
        };
        Self {
            state: Arc::new(Mutex::new(st)),
        }
    }

    pub async fn run_forever(self) -> Result<()> {
        let interval_secs = {
            let st = self.state.lock().await;
            st.core.cfg.interval_secs.max(5)
        };
        let interval = Duration::from_secs(interval_secs);

        info!(
            "EventEdgeClaudeFrameworkAgent started (interval={}s trade={})",
            interval.as_secs(),
            self.state.lock().await.core.cfg.trade
        );

        loop {
            if let Err(e) = self.run_one_cycle().await {
                warn!("EventEdgeClaudeFrameworkAgent cycle error: {}", e);
            }
            tokio::time::sleep(interval).await;
        }
    }

    async fn run_one_cycle(&self) -> Result<()> {
        let tools = self.build_tools();
        let mcp = create_sdk_mcp_server("event-edge", "1.0.0", tools);

        let mut mcp_map = HashMap::new();
        mcp_map.insert("event-edge".to_string(), McpServerConfig::Sdk(mcp));

        let (model, max_turns, trade) = {
            let st = self.state.lock().await;
            (
                st.core.cfg.model.clone(),
                st.core.cfg.claude_max_turns,
                st.core.cfg.trade,
            )
        };

        let sys = build_system_prompt();

        let mut options = ClaudeAgentOptions::builder()
            .system_prompt(SystemPrompt::Text(sys))
            .permission_mode(PermissionMode::BypassPermissions)
            .allowed_tools(vec![
                "event_edge_targets".to_string(),
                "event_edge_resolve_event".to_string(),
                "event_edge_scan".to_string(),
                "event_edge_buy_yes".to_string(),
            ])
            .mcp_servers(McpServers::Dict(mcp_map))
            .max_turns(max_turns)
            .continue_conversation(false)
            .build();

        if let Some(m) = model {
            options.model = Some(m);
        }
        if !trade {
            options
                .disallowed_tools
                .push("event_edge_buy_yes".to_string());
        }

        let mut client = ClaudeClient::new(options);
        client
            .connect()
            .await
            .map_err(|e| PloyError::Internal(format!("claude-agent connect failed: {e}")))?;

        client
            .query("Run one cycle.")
            .await
            .map_err(|e| PloyError::Internal(format!("claude-agent query failed: {e}")))?;

        let mut stream = client.receive_response();
        while let Some(item) = stream.next().await {
            match item
                .map_err(|e| PloyError::Internal(format!("claude-agent stream error: {e}")))?
            {
                Message::Assistant(msg) => {
                    for block in msg.message.content {
                        match block {
                            ContentBlock::Text(t) => info!("[Claude] {}", t.text),
                            ContentBlock::Thinking(t) => info!("[Claude thinking] {}", t.thinking),
                            _ => {}
                        }
                    }
                }
                Message::Result(r) => {
                    info!("[Claude result] {:?}", r.result);
                    break;
                }
                _ => {}
            }
        }

        drop(stream);
        client
            .disconnect()
            .await
            .map_err(|e| PloyError::Internal(format!("claude-agent disconnect failed: {e}")))?;
        Ok(())
    }

    fn build_tools(&self) -> Vec<claude_agent_sdk_rs::types::mcp::SdkMcpTool> {
        let state = Arc::clone(&self.state);
        let targets_tool = build_targets_tool(Arc::clone(&state));

        let state = Arc::clone(&self.state);
        let resolve_tool = build_resolve_tool(Arc::clone(&state));

        let state = Arc::clone(&self.state);
        let scan_tool = build_scan_tool(Arc::clone(&state));

        let state = Arc::clone(&self.state);
        let buy_tool = build_buy_tool(Arc::clone(&state));

        vec![targets_tool, resolve_tool, scan_tool, buy_tool]
    }
}

// ── MCP Tool builders ────────────────────────────────────────────────

fn build_targets_tool(
    state: Arc<Mutex<FrameworkState>>,
) -> claude_agent_sdk_rs::types::mcp::SdkMcpTool {
    tool!(
        "event_edge_targets",
        "Return configured event targets and risk parameters for the EventEdge agent.",
        json!({"type":"object","properties":{}}),
        move |_args: serde_json::Value| {
            let state = Arc::clone(&state);
            async move {
                let st = state.lock().await;
                Ok(claude_agent_sdk_rs::types::mcp::ToolResult {
                    content: vec![claude_agent_sdk_rs::types::mcp::ToolResultContent::Text {
                        text: serde_json::to_string_pretty(&json!({
                            "framework": st.core.cfg.framework,
                            "trade": st.core.cfg.trade,
                            "interval_secs": st.core.cfg.interval_secs,
                            "min_edge": st.core.cfg.min_edge.to_string(),
                            "max_entry": st.core.cfg.max_entry.to_string(),
                            "shares": st.core.cfg.shares,
                            "cooldown_secs": st.core.cfg.cooldown_secs,
                            "max_daily_spend_usd": st.core.cfg.max_daily_spend_usd.to_string(),
                            "event_ids": st.core.cfg.event_ids,
                            "titles": st.core.cfg.titles,
                        }))?,
                    }],
                    is_error: false,
                })
            }
        }
    )
}

fn build_resolve_tool(
    state: Arc<Mutex<FrameworkState>>,
) -> claude_agent_sdk_rs::types::mcp::SdkMcpTool {
    tool!(
        "event_edge_resolve_event",
        "Resolve a Polymarket event_id from a human title string using Gamma title_contains search.",
        json!({"type":"object","properties":{"title":{"type":"string"}},"required":["title"]}),
        move |args: serde_json::Value| {
            let state = Arc::clone(&state);
            async move {
                let title = args["title"].as_str().unwrap_or_default().to_string();
                let _ = state;
                let event_id = discover_best_event_id_by_title(&title).await?;
                Ok(claude_agent_sdk_rs::types::mcp::ToolResult {
                    content: vec![claude_agent_sdk_rs::types::mcp::ToolResultContent::Text {
                        text: serde_json::to_string_pretty(&json!({
                            "title": title,
                            "event_id": event_id
                        }))?,
                    }],
                    is_error: false,
                })
            }
        }
    )
}

fn build_scan_tool(
    state: Arc<Mutex<FrameworkState>>,
) -> claude_agent_sdk_rs::types::mcp::SdkMcpTool {
    tool!(
        "event_edge_scan",
        "Scan one Polymarket multi-outcome event for Arena-driven mispricing and return ranked opportunities.",
        json!({"type":"object","properties":{"event_id":{"type":"string"}},"required":["event_id"]}),
        move |args: serde_json::Value| {
            let state = Arc::clone(&state);
            async move {
                let event_id = args["event_id"].as_str().unwrap_or_default().to_string();
                let arena = fetch_arena_text_snapshot().await?;

                let scan = {
                    let st = state.lock().await;
                    scan_event_edge_once(&st.core.client, &event_id, Some(arena.clone())).await?
                };

                // Update token guard for tokens that clear thresholds.
                {
                    let mut st = state.lock().await;
                    st.core.reset_daily_if_needed();
                    let now = Utc::now();
                    let guard_ttl = Duration::from_secs(90);
                    let min_edge = st.core.cfg.min_edge;
                    let max_entry = st.core.cfg.max_entry;
                    for r in &scan.rows {
                        let (Some(ask), Some(edge)) = (r.market_ask, r.edge) else {
                            continue;
                        };
                        if ask > max_entry || edge < min_edge {
                            continue;
                        }
                        if r.ev.as_ref().map(|e| e.is_positive_ev).unwrap_or(false) != true {
                            continue;
                        }
                        st.token_guard.insert(
                            r.yes_token_id.clone(),
                            TokenGuard {
                                ok_to_buy_until: now
                                    + chrono::Duration::from_std(guard_ttl).unwrap_or_default(),
                                min_edge,
                                last_seen_ask: ask,
                                last_seen_p_true: r.p_true,
                                outcome: r.outcome.clone(),
                                event_id: event_id.clone(),
                            },
                        );
                    }
                    st.token_guard.retain(|_, g| g.ok_to_buy_until > now);
                }

                Ok(claude_agent_sdk_rs::types::mcp::ToolResult {
                    content: vec![claude_agent_sdk_rs::types::mcp::ToolResultContent::Text {
                        text: serde_json::to_string_pretty(&json!({
                            "event_id": scan.event_id,
                            "event_title": scan.event_title,
                            "end_time": scan.end_time.to_rfc3339(),
                            "confidence": scan.confidence,
                            "arena_last_updated": scan.arena_last_updated.map(|d| d.to_string()),
                            "rows": scan.rows.iter().take(12).map(edge_row_to_json).collect::<Vec<_>>(),
                        }))?,
                    }],
                    is_error: false,
                })
            }
        }
    )
}

fn build_buy_tool(
    state: Arc<Mutex<FrameworkState>>,
) -> claude_agent_sdk_rs::types::mcp::SdkMcpTool {
    tool!(
        "event_edge_buy_yes",
        "Place a YES-side limit buy for a given token_id. Enforces cooldown + daily spend cap + requires a recent scan guard.",
        json!({"type":"object","properties":{
            "token_id":{"type":"string"},
            "shares":{"type":"integer","minimum":1},
            "limit_price":{"type":"number","minimum":0,"maximum":1}
        },"required":["token_id","shares","limit_price"]}),
        move |args: serde_json::Value| {
            let state = Arc::clone(&state);
            async move {
                let token_id = args["token_id"].as_str().unwrap_or_default().to_string();
                let shares = args["shares"].as_u64().unwrap_or(0);
                let limit_price = args["limit_price"].as_f64().unwrap_or(0.0);
                let limit_price = Decimal::from_f64(limit_price).unwrap_or(Decimal::ZERO);

                if token_id.is_empty() || shares == 0 {
                    return Ok(tool_err("invalid_args", "token_id and shares required"));
                }

                let mut st = state.lock().await;
                st.core.reset_daily_if_needed();

                if st.core.is_on_cooldown(&token_id) {
                    return Ok(tool_err("cooldown", "cooldown active for token"));
                }

                let Some(guard) = st.token_guard.get(&token_id).cloned() else {
                    return Ok(tool_err(
                        "guard",
                        "no recent scan guard for token_id (run event_edge_scan first)",
                    ));
                };
                if guard.ok_to_buy_until <= Utc::now() {
                    return Ok(tool_err("guard_expired", "scan guard expired; rescan required"));
                }

                let notional = Decimal::from(shares) * limit_price;
                if !st.core.can_spend(notional) {
                    return Ok(tool_err("daily_cap", "would exceed max_daily_spend_usd"));
                }

                if limit_price > guard.last_seen_ask + dec!(0.02) {
                    return Ok(tool_err(
                        "price_too_high",
                        "limit_price exceeds last seen ask by >2c",
                    ));
                }

                let order =
                    OrderRequest::buy_limit(token_id.clone(), Side::Up, shares, limit_price);
                let resp = st.core.client.submit_order(&order).await?;

                st.core.record_trade(&token_id, notional);

                Ok(claude_agent_sdk_rs::types::mcp::ToolResult {
                    content: vec![claude_agent_sdk_rs::types::mcp::ToolResultContent::Text {
                        text: serde_json::to_string_pretty(&json!({
                            "ok": true,
                            "event_id": guard.event_id,
                            "outcome": guard.outcome,
                            "shares": shares,
                            "limit_price": limit_price.to_string(),
                            "edge_required": guard.min_edge.to_string(),
                            "order_id": resp.id,
                            "status": resp.status,
                            "daily_spend_usd": st.core.state.daily_spend_usd.to_string(),
                        }))?,
                    }],
                    is_error: false,
                })
            }
        }
    )
}

// ── Helpers ──────────────────────────────────────────────────────────

fn build_system_prompt() -> String {
    "You are an autonomous trading agent.\n\
     Objective: detect and trade Polymarket mispricings for AI leaderboard markets.\n\
     \n\
     You MUST use the provided tools (event_edge_targets, event_edge_resolve_event, \
     event_edge_scan, event_edge_buy_yes).\n\
     Do NOT use any other tools.\n\
     \n\
     Policy:\n\
     - Always start by calling event_edge_targets.\n\
     - For each configured title/event_id, call event_edge_resolve_event (if needed) \
     then event_edge_scan.\n\
     - Only call event_edge_buy_yes when it is justified by the scan output.\n\
     - If trade is disabled, do not call event_edge_buy_yes.\n\
     - Keep actions conservative. Prefer 0 or 1 trades per cycle.\n\
     \n\
     At the end, output a short plain-text summary of what you did in this cycle."
        .to_string()
}

fn edge_row_to_json(r: &EdgeRow) -> serde_json::Value {
    json!({
        "outcome": r.outcome,
        "yes_token_id": r.yes_token_id,
        "ask": r.market_ask.map(|v| v.to_string()),
        "p_true": r.p_true.to_string(),
        "edge": r.edge.map(|v| v.to_string()),
        "net_ev": r.ev.as_ref().map(|e| e.net_ev.to_string()),
    })
}

fn tool_err(code: &str, message: &str) -> claude_agent_sdk_rs::types::mcp::ToolResult {
    claude_agent_sdk_rs::types::mcp::ToolResult {
        content: vec![claude_agent_sdk_rs::types::mcp::ToolResultContent::Text {
            text: serde_json::to_string_pretty(&json!({
                "ok": false,
                "code": code,
                "message": message,
            }))
            .unwrap_or_else(|_| {
                format!("{{\"ok\":false,\"code\":\"{code}\",\"message\":\"{message}\"}}")
            }),
        }],
        is_error: true,
    }
}
