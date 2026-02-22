# crypto_lob_ml 架构层 vs 策略层边界

## 1. 架构层（System / Runtime）
- `PlatformBootstrapConfig::from_app_config` 负责把环境变量注入策略配置（如 `MODEL_TYPE/MODEL_PATH/MODEL_VERSION`）。
- 模型加载失败时的运行模式属于架构层：
  - `PLOY_CRYPTO_LOB_ML__COLLECT_ONLY_ON_MODEL_ERROR=true` 时，策略进入 collect-only（不发 entry 订单）。
- 风控和下单准入由 Coordinator 统一负责（本次未改变这层职责）。

## 2. 策略层（Signal / Decision）
- `crypto_lob_ml` 用 LOB 特征输出 `p_up_model`。
- `p_up_window` 仅作为 safety fallback 小权重参与，不再主导。
- 当前默认混合：`p_up = 0.90 * p_up_model + 0.10 * p_up_window`（可用 env 调整窗口权重）。

## 3. 训练层（Offline ML）
- 新训练入口：`scripts/train_crypto_lob_mlp_onnx_from_db.py --source sync_records --horizon 5m|15m`。
- 标签来自 `pm_token_settlements`（resolved 结果），特征来自 `sync_records`（LOB + 短周期动量）。
- 训练产物通过 `MODEL_TYPE/MODEL_PATH/MODEL_VERSION` 接入 runtime。

## 4. 上线清单
- 参考：`docs/CRYPTO_LOB_ML_DEPLOY_CHECKLIST.md`
