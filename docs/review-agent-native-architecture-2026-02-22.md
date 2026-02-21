# Agent-Native Architecture Review (2026-02-22)

## Scope

Review target: current platform runtime (`Coordinator + agents + sidecar/governance API`) with OpenClaw as control plane.

## Current Layering Verdict

結論：方向是對的，但還有兩個結構性缺口要持續收斂。

- 上層（AI 調度 / OpenClaw）：透過 governance + deployments + system control API 做治理。
- 中層（Execution Plane / Coordinator）：唯一 live 下單入口，負責 gate/queue/risk/execution/audit。
- 下層（Rust adapters/executor）：與 Polymarket/CLOB/DB/WS 互動，純執行與資料管線。

這個分層已經符合「OpenClaw 不逐筆審批，Poly Agent 自主決策」的原則。

## Agent-Native Scorecard

| Principle | Score | Status | Notes |
|---|---:|---|---|
| Action Parity | 82% | ✅ | 控制面核心動作（部署、治理、pause/resume/halt、intent ingress）可 API 化 |
| Tools as Primitives | 78% | ⚠️ | Coordinator 與 sidecar ingress 已偏 primitive；仍有 legacy command workflow code 混在同 binary |
| Context Injection | 70% | ⚠️ | `governance/status` + `strategies/control` 提供控制面上下文，但策略優化上下文仍分散 |
| Shared Workspace | 88% | ✅ | 控制面與 runtime 共享 DB/部署矩陣/治理狀態 |
| CRUD Completeness | 87% | ✅ | Deployments/Governance + strategies control 已有讀與定向更新；已補策略 version/lifecycle 契約 |
| UI Integration | 74% | ⚠️ | WebSocket + API 可觀測；控制面全景在 UI 還不完整 |
| Capability Discovery | 76% | ⚠️ | 已新增 `/api/capabilities` 機器可讀能力發現；仍缺 UI 端引導 |
| Prompt-Native Features | 70% | ⚠️ | 已支持 AI sidecar 調度，但策略行為仍大量硬編碼在 Rust agent |

**Overall: 81% (Partial, architecture is viable for staged production).**

## What Was Fixed In This Pass

1. `CoordinatorHandle::force_close_domain` / `shutdown_domain` 現在會先即時把 domain ingress 設為 `halted`，避免命令傳遞延遲期間繼續吃 BUY intents。
2. `GET /api/governance/status` 擴展為調度友好快照：
   - `domain_ingress_modes[]`
   - `agents[]`（含 heartbeat/exposure/pnl/error）
3. 補上對應回歸測試（domain halt 即時生效 + governance status 新欄位）。
4. `StrategyDeployment` 補上控制面契約欄位：`strategy_version` / `lifecycle_stage` / `product_type` / `last_evaluated_at` / `last_evaluation_score`。
5. `GET/PUT /api/strategies/control*` 可讀寫 lifecycle/version，且 sidecar live ingress 預設只接受 `lifecycle_stage=live`。

## Remaining High-Impact Gaps

1. `main.rs` 仍承載大量 legacy mode，雖然 live path 已收斂到 coordinator，但程式結構仍過重。
2. `src/agent` / `src/agents` / `src/platform/agents` 三套命名並存，對新策略接入有認知成本。
3. lifecycle 現在已可治理，但「回測證據/線上驗證報告」仍未形成標準化資料模型（目前只到 score/timestamp）。

## Recommended Next Refactors (ordered)

1. 拆分 `main.rs`：保留 platform runtime 入口，legacy runner 移入 `bin/legacy_*`。
2. 統一 agent namespace：收斂到單一 `agents/`（保留 adapter 與 domain 子模組）。
3. 補 `strategy_evaluations` 持久層（backtest/paper/live metrics + dataset hash + model hash），把 lifecycle gate 從欄位治理升級為證據治理。
